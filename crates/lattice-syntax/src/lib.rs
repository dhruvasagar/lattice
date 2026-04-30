//! Tree-sitter-backed syntax highlighting for `lattice` (DESIGN.md §5.3).
//!
//! Phase 3 status: full reparse on every edit (synchronous). Incremental
//! reparse via `Tree::edit` and async reparse on `spawn_blocking` workers
//! arrive once the actor model lands -- the seam is `Syntax::parse`, which
//! today rebuilds from scratch but can be swapped to take an `InputEdit`
//! without changing callers.
//!
//! Languages bundled in this revision: Rust, Python, JavaScript, plus a
//! `Plain` no-op fallback. Adding a new language is a matter of:
//!
//! 1. Adding the `tree-sitter-<lang>` crate as a dep.
//! 2. Adding a variant to `Lang`.
//! 3. Wiring `Lang::detect_from_path` and `Lang::config` for it.
//!
//! Capture-name -> `Style` mapping lives in `style.rs`. The mapping is the
//! v1 stand-in for the themable name-to-color tables described in §5.6
//! style mappings.

pub mod lang;
pub mod style;
pub mod syntax;

pub use crate::lang::Lang;
pub use crate::style::{Style, StyledSpan};
pub use crate::syntax::{Syntax, SyntaxError};
