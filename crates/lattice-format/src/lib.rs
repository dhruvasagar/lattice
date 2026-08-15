//! External formatters, and how their output reaches a buffer.
//!
//! The external-tool half of auto-indent
//! (`docs/dev/architecture/auto-indent.md` §8). Where the indent engine
//! answers "which column does this line start at" synchronously from a
//! parse tree, this runs somebody else's program and turns its opinion
//! into edits.
//!
//! Three concerns, three modules, because they fail differently:
//!
//! - [`spec`] — which program, per language. Data.
//! - [`runner`] — spawning it, with a timeout and a typed error per
//!   failure mode. Blocking; the caller puts it on `spawn_blocking`.
//! - [`apply`] — whole-file output → a **minimal** edit set, never a
//!   buffer-wide replace.
//!
//! ## Why this is a crate and the indent engine is not
//!
//! `lattice-indent` was proposed, built and collapsed into
//! `lattice-syntax` during IN.1: a pure tree walk driven by `.scm`
//! files belongs beside the other tree walks. This is genuinely
//! different work — process spawning, a timeout policy, stderr
//! handling, diff-based edit derivation — and none of it belongs in a
//! syntax crate.

pub mod apply;
pub mod runner;
pub mod spec;

pub use apply::{changes_more_than_indentation, minimal_edits};
pub use runner::{FORMAT_TIMEOUT, FormatError, run};
pub use spec::FormatterSpec;
