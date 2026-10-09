//! The editor's keymap engine: the chord trie, the layered runtime
//! registry every keystroke resolves against, the built-in vim keymap
//! catalog, and the introspection models (`:describe-key`, which-key)
//! derived from them.
//!
//! ## What it owns
//!
//! - **Matching** — [`KeymapTrie`]: one layer's bindings, `O(prefix)`
//!   lookup to [`LookupResult::Bound`] / `Partial` / `Unbound`, with a
//!   `{char}` wildcard ([`ChordPattern::CharLiteral`]) for marks,
//!   registers and find-char.
//! - **Layering** — [`KeymapRegistry`] behind [`KeymapHandle`]: five
//!   [`KeymapLayer`]s (`Builtin < MajorMode < MinorMode < User < Buffer`),
//!   one trie per [`BindingMode`] per layer, wait-free reads and
//!   capability-gated writes ([`KeymapCapability`]). Mode layers are
//!   gated by the active buffer's [`ModeId`]s
//!   ([`KeymapHandle::lookup_with_context`]).
//! - **Declaration** — [`Keymap`] / [`KeymapBinding`], a mode's
//!   declarative contribution, and the [`keymap_entry!`] static-table form
//!   ([`KeymapEntry`]) with the built-in catalog in [`default_keymap`].
//! - **Introspection** — [`KeymapResolution`] / [`Continuation`] for
//!   `:describe-key`; [`which_key`]'s [`WhichKeyModel`] and layout; the
//!   [`PartialChordPending`] event which-key subscribes to.
//!
//! ## What it must not depend on
//!
//! Dependency position in the workspace:
//!   lattice-protocol → lattice-grammar → lattice-keymap
//!     → lattice-mode → lattice-host
//!
//! Nothing in this crate may import from `lattice-mode` or `lattice-host`.
//! It is its own crate so the trie, the layer enum and the binding types
//! can be named by `lattice-mode` (whose `Mode::keymap` returns a
//! [`Keymap`]) without a cycle, and so the keystroke-path matcher carries
//! no editor state, renderer or I/O — everything here is testable with a
//! hand-built trie and no host.
//!
//! # Examples
//!
//! Bind a builtin chord and a mode override, then resolve a keystroke the
//! way the dispatcher does:
//!
//! ```
//! use lattice_grammar::{CommandId, CommandInvocation, SourceLocation};
//! use lattice_keymap::{
//!     BindingMode, KeymapCapability, KeymapHandle, KeymapLayer, LookupResult, ModeId,
//! };
//! use lattice_protocol::parse_chord_sequence;
//!
//! let keymap = KeymapHandle::new();
//! let bind = |layer, chord, id| {
//!     keymap.try_bind_chord_string(KeymapCapability::Full, layer, BindingMode::Normal, chord,
//!         CommandInvocation::of(CommandId::new(id)), SourceLocation::synthetic("doc")).unwrap()
//! };
//! bind(KeymapLayer::Builtin, "<C-w>v", 1);
//! bind(KeymapLayer::MinorMode(ModeId::new("magit-mode")), "<C-w>v", 2);
//!
//! let keys = parse_chord_sequence("<C-w>v").unwrap();
//! let resolve = |active: &[ModeId]| match keymap.lookup_with_context(BindingMode::Normal, &keys, active) {
//!     LookupResult::Bound { command, .. } => command.command.command,
//!     other => panic!("{other:?}"),
//! };
//! assert_eq!(resolve(&[]), CommandId::new(1));
//! assert_eq!(resolve(&[ModeId::new("magit-mode")]), CommandId::new(2));
//!
//! // `:describe-key` sees both layers, and which one fires here.
//! let trace = keymap.resolve_trace(BindingMode::Normal, &keys, &[]);
//! assert_eq!(trace.hits.len(), 2);
//! assert_eq!(trace.winner().unwrap().layer, KeymapLayer::Builtin);
//! ```
//!
//! ## Design
//!
//! - `docs/dev/architecture/keymap-architecture.md` — layers, merge on
//!   write, capabilities, the motion mirror.
//! - `docs/dev/architecture/which-key.md` — the [`which_key`] model.
//! - `docs/dev/architecture/design.md` §5.2.3 — the five-layer model.

#![warn(missing_docs)]

pub mod binding_mode;
pub mod contribution;
pub mod keymap_entry;
pub mod mode_id;

pub use binding_mode::BindingMode;
pub use contribution::{Keymap, KeymapBinding};
pub use keymap_entry::{KeymapEntry, default_keymap, entries, lookup};
pub use mode_id::ModeId;

pub mod trie;
pub use lattice_protocol::ChordPattern;
pub use trie::{BoundCommand, KeymapLayer, KeymapTrie, LookupResult};

pub mod registry;
pub use registry::{
    DEFAULT_LEADER, KeymapCapability, KeymapError, KeymapHandle, KeymapRegistry, LayerId,
    PushLayerKind, expand_leader, overtypes_in_select,
};

pub mod resolution;
pub use resolution::{
    Continuation, KeymapResolution, LayerHit, describe_key_mode_for_letter, parse_describe_key_arg,
};

pub mod events;
pub use events::PartialChordPending;

pub mod which_key;
pub use trie::{ChildView, NodeView};
pub use which_key::{Entry, EntryKind, Sort, WhichKeyModel, build_model};
