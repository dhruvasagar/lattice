//! TextMate-format snippet engine for lattice
//! (Phase 4.2.g.4; design in
//! [`docs/dev/architecture/insert-completion.md`](../../docs/dev/architecture/insert-completion.md) §8).
//!
//! Three responsibilities:
//!
//! 1. **Parse** TextMate / LSP snippet body syntax (placeholders,
//!    choices, variables, escapes) into a stream of [`SnippetToken`]s.
//! 2. **Render** that stream into the literal text the editor
//!    inserts plus the per-tabstop byte ranges
//!    ([`RenderedSnippet`]) -- so the host can wire placeholder
//!    navigation without re-parsing.
//! 3. **Track** the in-flight snippet expansion's tabstop state
//!    via [`ActiveSnippet`] so `<Tab>` / `<S-Tab>` step through
//!    placeholders and edits in one tabstop ripple to its
//!    mirrors.
//!
//! Drop-in compatibility with the **VS Code TextMate JSON
//! format** is the design target: `friendly-snippets` (the
//! community-maintained corpus most editors pull from) loads
//! verbatim. See [`load`] for the JSON ingestion path and
//! [`registry::SnippetRegistry`] for per-language storage.
//!
//! Variables are resolved against an editor-supplied
//! [`VariableContext`] -- the host fills in `TM_FILENAME` /
//! `CLIPBOARD` / etc. when the snippet expands. v1 supports the
//! TextMate / VS Code built-in variable set; transformation
//! placeholders (`${1/pattern/replacement/}`) parse to a
//! transformation token but apply as their original text in
//! v1; full regex transformation lands as polish.
//!
//! This crate has **no editor dependency**. Tests parse + render
//! + drive the active-snippet state machine without touching
//! any rope or buffer; the host wires [`ActiveSnippet`] to its
//! buffer-mutation pipeline.

pub mod active;
pub mod load;
pub mod parse;
pub mod registry;
pub mod render;
pub mod token;
pub mod variables;

pub use crate::active::{ActiveSnippet, TabstopGroup};
pub use crate::load::{LoadError, load_pack, load_pack_from_str};
pub use crate::parse::{ParseError, parse};
pub use crate::registry::{Snippet, SnippetMeta, SnippetRegistry};
pub use crate::render::{RenderedSnippet, TabstopRange};
pub use crate::token::{ChoiceOption, SnippetBody, SnippetToken};
pub use crate::variables::{VariableContext, builtin_variables};
