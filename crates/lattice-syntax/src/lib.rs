//! Tree-sitter-backed syntax highlighting for `lattice` (DESIGN.md §5.3).
//!
//! ## Status
//!
//! - Languages bundled: Rust, Python, JavaScript, Markdown (block +
//!   inline split), plus a `Plain` no-op fallback.
//! - **Incremental reparse landed (Option B, slices B.1–B.5).**
//!   `Syntax::parse_at_with_edits` accepts `EditDelta`s, applies
//!   `tree.edit()` to the cached tree, and runs
//!   `Parser::parse(_, Some(&old_tree))` so unchanged subtrees are
//!   reused. `SyntaxHandle` runs the parse on a `spawn_blocking`
//!   worker; `request_reparse` takes `Buffer` (O(1) Arc bump) so
//!   the input thread doesn't allocate the source. App-side cache
//!   on `App.visible_highlights_key` short-circuits the per-frame
//!   `highlight_lines` walk when nothing changed.
//! - **Flicker-free updates landed (C-series, slices C.1–C.5).**
//!   The C-series turns the algorithmically-correct B-series into
//!   a user-visibly-flicker-free experience. The two-stage parse
//!   exposes `Syntax::try_apply_intermediate` (fast: tree.edit +
//!   source/version update, no parse) and `reparse_with_cached_tree`
//!   (slow: Parser::parse with cached tree as seed) so the
//!   `SyntaxHandle` worker can publish a byte-aligned intermediate
//!   snapshot before the parse completes. `App` synchronously
//!   line-shifts and byte-shifts its `visible_highlights` cache on
//!   every edit so held spans stay aligned with current content;
//!   the renderer never sees an empty/wrong intermediate. Grammar-
//!   driven edits (operators) flow through the same chokepoint
//!   so the C.x logic applies uniformly.
//! - Plugin extension API used by builtins, not yet by plugins.
//!
//! Adding a new language:
//!
//! 1. Add the `tree-sitter-<lang>` crate as a dep.
//! 2. Register it in [`registry::LangRegistry::standard`].
//! 3. Add a variant to `Lang` (and update `Lang::detect_from_path`
//!    for the canonical extension).
//!
//! ## Injections
//!
//! The shared [`registry::LangRegistry`] holds every registered
//! grammar's `HighlightConfiguration`; the per-document `Syntax`
//! borrows refs through it. The injection callback looks up
//! sibling configs by name -- so a markdown `\`\`\`rust ... \`\`\``
//! block recurses into the rust config, and a markdown paragraph
//! injects the inline-markdown parser. New languages drop in to
//! the registry and become injection targets without further
//! wiring.
//!
//! Capture-name -> `Style` mapping lives in `style.rs`. The mapping
//! is the v1 stand-in for the themable name-to-color tables
//! described in §5.6 style mappings.

pub mod handle;
pub mod lang;
pub mod modes;
pub mod oneshot;
pub mod registry;
pub mod style;
pub mod syntax;

pub use crate::handle::SyntaxHandle;
pub use crate::lang::Lang;
pub use crate::modes::{
    JavascriptMode, MarkdownMode, PythonMode, RustMode, TREE_SITTER_COMPLETION_SOURCE_ID,
    TreeSitterCompletionMode, TreeSitterSymbolSource, major_mode_id_for_lang,
    register_language_modes,
};
pub use crate::oneshot::oneshot_highlight_lines;
pub use crate::registry::LangRegistry;
pub use crate::style::{Style, StyledSpan};
pub use crate::syntax::{Syntax, SyntaxError, SyntaxSnapshot};
