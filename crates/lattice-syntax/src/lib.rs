//! Tree-sitter-backed syntax highlighting for `lattice` (DESIGN.md §5.3).
//!
//! Phase 3 status: full reparse on every edit (synchronous). Incremental
//! reparse via `Tree::edit` and async reparse on `spawn_blocking` workers
//! arrive once the actor model lands -- the seam is `Syntax::parse`, which
//! today rebuilds from scratch but can be swapped to take an `InputEdit`
//! without changing callers.
//!
//! Languages bundled in this revision: Rust, Python, JavaScript,
//! Markdown (block + inline split), plus a `Plain` no-op fallback.
//! Adding a new language is a matter of:
//!
//! 1. Adding the `tree-sitter-<lang>` crate as a dep.
//! 2. Registering it in [`registry::LangRegistry::standard`].
//! 3. Adding a variant to `Lang` (and updating
//!    `Lang::detect_from_path` for the canonical extension).
//!
//! ## Injections
//!
//! The shared [`registry::LangRegistry`] holds every registered
//! grammar's `HighlightConfiguration`; the per-document `Syntax`
//! borrows refs through it. The `injection_callback` in
//! `Highlighter::highlight` looks up sibling configs by name -- so
//! a markdown `\`\`\`rust ... \`\`\`` block recurses into the rust
//! config, and a markdown paragraph injects the inline-markdown
//! parser. New languages drop in to the registry and become
//! injection targets without further wiring.
//!
//! Capture-name -> `Style` mapping lives in `style.rs`. The mapping is the
//! v1 stand-in for the themable name-to-color tables described in §5.6
//! style mappings.

pub mod lang;
pub mod registry;
pub mod style;
pub mod syntax;

pub use crate::lang::Lang;
pub use crate::registry::LangRegistry;
pub use crate::style::{Style, StyledSpan};
pub use crate::syntax::{Syntax, SyntaxError};
