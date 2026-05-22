//! Major modes for buffer kinds owned by the TUI layer.
//!
//! Three majors corresponding to the existing `BufferKind`
//! variants beyond `Document`:
//!
//! - `help-mode` -- `:describe-*` / `:apropos` / `:keymap` views.
//!   Read-only; markdown-mode-style content with link
//!   navigation (`<CR>` follows a link).
//! - `file-tree-mode` -- the file-tree navigation buffer.
//!   Tree expansion / collapse, open-on-`<CR>`.
//! - `oil-mode` -- editable directory listing
//!   (oil.nvim-style). Writable; `:w` diffs the rope and
//!   applies filesystem ops.
//!
//! Pure declarations in M.3.0. The actual behavior currently
//! lives in scattered call sites in `app.rs` / `render.rs` /
//! `input.rs`; M.3.1 routes those through the mode-id queries
//! and M.4 unifies rendering through `ResolvedOptions`.
//!
//! Per `mode-architecture.md` §4.1 the TUI also hosts
//! `command-line-mode` and `search-line-mode` for the rich
//! minibuffer (DESIGN.md §5.9.10), but the rich minibuffer
//! refactor isn't landed yet -- those modes are deferred to
//! the slice that ships them.

use lattice_mode::{BufferLocal, ModeId, ModeRegistry, TextMode};
use lattice_syntax::Lang;

use crate::buffers::BufferKind;
use crate::help::{HelpAnchor, HelpLink};

// ---- M.3.2.c.5: help-mode buffer-locals ----
//
// Three newtypes carrying the per-buffer mode-internal data
// help-mode owns. Canonical owner of help per-buffer state per
// M.3.2.c.5 -- the `HelpBuffer` struct no longer holds these
// fields; the App seeds them into `buffer_locals[id]` at
// popup-open time via `seed_help_metadata_locals`. Each
// newtype's `OWNER_MODE` is `"help-mode"` so
// `:describe-buffer` attributes them correctly.

/// Help buffer's parsed `[label](url)` markdown links.
#[derive(Debug, Clone)]
pub struct HelpLinks(pub Vec<HelpLink>);

impl BufferLocal for HelpLinks {
    const NAME: &'static str = "help-mode.links";
    const DOC: &'static str = "Parsed `[label](url)` markdown links; produced at \
         buffer construction by the help-text parser.";
    const OWNER_MODE: &'static str = "help-mode";
    fn describe(&self) -> String {
        format!("{} link(s)", self.0.len())
    }
}

/// Help buffer's named anchors (heading slugs +
/// introspection-recorded anchors).
#[derive(Debug, Clone)]
pub struct HelpAnchors(pub Vec<HelpAnchor>);

impl BufferLocal for HelpAnchors {
    const NAME: &'static str = "help-mode.anchors";
    const DOC: &'static str = "Named scroll targets inside the help body. Heading \
         slugs auto-generated; introspection renderers may add \
         additional `kind:name` anchors.";
    const OWNER_MODE: &'static str = "help-mode";
    fn describe(&self) -> String {
        format!("{} anchor(s)", self.0.len())
    }
}

/// Help buffer's pre-computed per-line markdown highlight
/// spans. Empty when the buffer was constructed without a
/// language registry (test paths).
#[derive(Debug, Clone)]
pub struct HelpHighlights(pub Vec<Vec<lattice_syntax::StyledSpan>>);

impl BufferLocal for HelpHighlights {
    const NAME: &'static str = "help-mode.highlights";
    const DOC: &'static str = "Pre-computed per-line markdown highlight spans, indexed \
         by visible line. Populated by `with_markdown_syntax`.";
    const OWNER_MODE: &'static str = "help-mode";
    fn describe(&self) -> String {
        format!("{} line(s) of highlights", self.0.len())
    }
}

// ---- file-tree-mode buffer-locals ----
//
// All three live in `lattice_file_tree::modes` alongside their
// declaring mode (`FileTreeMode`). Re-exported here so existing
// `crate::modes::FileTreeRoot` etc. callsites compile without
// change; new callers should import from `lattice_file_tree`
// directly.

pub use lattice_file_tree::{FileTreeEntries, FileTreeNerdFonts, FileTreeRoot};

// ---- M.3.2.c.3: oil-mode buffer-locals ----
//
// `OilDir` moved to `lattice_oil::modes` alongside its
// declaring mode (`OilMode`). Re-exported here so existing
// `crate::modes::OilDir` callsites compile without change;
// new callers should import from `lattice_oil` directly.

pub use lattice_oil::OilDir;

// ---- M.3.2.c.4: language-mode / text-mode buffer-locals ----
//
// `DocumentEntry` carries four pieces of mode-internal state:
// - `syntax: Option<SyntaxHandle>` -- per-language major mode's
//   tree-sitter handle (incremental parse + highlight surface).
// - `last_parsed_text_version` / `last_synced_syntax_version` --
//   the syntax pipeline's edit-coalescing baselines.
// - `folds: Vec<Fold>` -- universal across language majors;
//   foundational text-mode owns it.
//
// These live as buffer-locals so a future language major (e.g.
// `RustMode` registering a richer indent / fold profile) can
// declare ownership without touching `DocumentEntry`'s shape.
//
// The OWNER_MODE attribution is `"text-mode"` for the foundational
// data; `:describe-buffer` will show that. Once each language
// mode declares richer per-language state (M.4+), we can split
// the syntax handle's owner attribution per-language without
// changing the local's shape.

/// Per-document tree-sitter syntax handle. `Some` once a language
/// has been detected for the buffer and a parse has been
/// requested; `None` for `Lang::Plain` documents and for buffers
/// that haven't been activated yet.
#[derive(Debug, Clone)]
pub struct DocumentSyntax(pub Option<lattice_syntax::SyntaxHandle>);

impl BufferLocal for DocumentSyntax {
    const NAME: &'static str = "text-mode.syntax";
    const DOC: &'static str = "Per-document tree-sitter syntax handle. Drives the highlight \
         walk + the fold computation. Owned by the document's active \
         language major (text-mode for `Lang::Plain`, the matching \
         language mode otherwise); the handle's runtime data lives in \
         a worker actor referenced by this slot.";
    const OWNER_MODE: &'static str = "text-mode";
    fn describe(&self) -> String {
        match &self.0 {
            Some(_) => "attached".to_string(),
            None => "(none)".to_string(),
        }
    }
}

/// Document version (the rope's monotonic edit counter) of the
/// most recent successful parse. The renderer's syntax walk
/// short-circuits when `text_version == last_parsed_text_version`;
/// the reparse seam treats inequality as "edits to drain into the
/// worker" and fires an incremental reparse.
#[derive(Debug, Clone, Copy)]
pub struct DocumentLastParsedTextVersion(pub u64);

impl BufferLocal for DocumentLastParsedTextVersion {
    const NAME: &'static str = "text-mode.last-parsed-text-version";
    const DOC: &'static str = "Document version (rope monotonic) of the most recent \
         successful parse. Cheap idempotency check on the highlight \
         hot path: equal version means the cached spans are still \
         current; unequal triggers a reparse.";
    const OWNER_MODE: &'static str = "text-mode";
    fn describe(&self) -> String {
        format!("v{}", self.0)
    }
}

/// Document version that was the baseline for the last syntax-
/// worker request fired for this buffer. Sent as `from_version`
/// on each request so the worker can verify edits apply to the
/// expected tree baseline before running the incremental
/// `tree.edit()`. Independent of `last_parsed_text_version`
/// because the worker may still be in flight when the next
/// request fires.
#[derive(Debug, Clone, Copy)]
pub struct DocumentLastSyncedSyntaxVersion(pub u64);

impl BufferLocal for DocumentLastSyncedSyntaxVersion {
    const NAME: &'static str = "text-mode.last-synced-syntax-version";
    const DOC: &'static str = "Document version baseline most recently sent to the syntax \
         worker. Worker uses it as `from_version` to verify edits \
         apply against the expected tree before running tree.edit().";
    const OWNER_MODE: &'static str = "text-mode";
    fn describe(&self) -> String {
        format!("v{}", self.0)
    }
}

/// Per-document fold list. Empty means "not yet computed for this
/// buffer." First-activation seeds from the active foldmethod;
/// subsequent re-activations restore the user's open / closed
/// state. Universal across language majors -- the foundational
/// text-mode owns the data shape; per-language fold queries are
/// a presentation-layer concern read through the syntax local.
#[derive(Debug, Clone, Default)]
pub struct DocumentFolds(pub Vec<lattice_core::Fold>);

impl BufferLocal for DocumentFolds {
    const NAME: &'static str = "text-mode.folds";
    const DOC: &'static str = "Per-document fold list. Empty until the activation hook \
         seeds from the buffer's foldmethod; thereafter holds the \
         user's open / closed state across buffer-switch \
         round-trips.";
    const OWNER_MODE: &'static str = "text-mode";
    fn describe(&self) -> String {
        format!("{} fold(s)", self.0.len())
    }
}

// M.4 follow-up: HelpMode / HoverMode / FileTreeMode / OilMode
// migrated to `lattice-mode::modes` (one file per mode). The
// dep-inversion that made this possible: layer-input types
// (`OptionOverride{,Set}`, `OverridePriority`) live in
// `lattice-config`, `lattice-mode` depends on `lattice-config`,
// and `Document::modes` was removed from `lattice-core` so the
// cycle through `lattice-core -> lattice-mode` is gone.
//
// Re-exported here so existing `crate::modes::HelpMode` etc.
// imports keep working during the transition.
pub use lattice_mode::{HelpMode, HoverMode};
// Feature-crate-owned modes (moved per the mode-architecture
// convention; see each feature crate's `modes.rs`).
// Re-exported here so `crate::modes::*` callsites keep
// compiling; new code imports from the feature crate.
pub use lattice_file_tree::FileTreeMode;
pub use lattice_oil::OilMode;

/// Resolve the default major-mode id for a [`BufferKind`].
/// `Document` returns `None` because the mode is determined
/// by language detection (see
/// [`lattice_syntax::major_mode_id_for_lang`]); when the
/// language detection returns `Lang::Plain` the caller falls
/// back further to `text-mode`.
pub fn major_mode_id_for_buffer_kind(kind: BufferKind) -> Option<ModeId> {
    match kind {
        BufferKind::Document => None,
        // M.4 (Option B): help buffers run `markdown-mode` as the
        // major (the markdown content drives motion + syntax) and
        // pick up `help-mode` as a minor at activation time. The
        // minor adds the help-only behaviour (ReadOnly, links,
        // anchors, `:help`-family commands) without forking the
        // major-mode chassis.
        BufferKind::Help => Some(lattice_syntax::MarkdownMode::mode_id()),
        BufferKind::FileTree => Some(FileTreeMode::mode_id()),
        BufferKind::Oil => Some(OilMode::mode_id()),
        // Terminal: T1 has no major mode. T2 introduces
        // `terminal-mode` (the major that owns Normal-in-terminal
        // grammar + the Terminal-Insert sub-state).
        BufferKind::Terminal => None,
    }
}

/// M.4 (Option B): minor modes the App activates alongside the
/// major resolved by [`major_mode_id_for_buffer_kind`]. Returns
/// the per-kind minor that gives the buffer its full identity.
/// Help buffers get `help-mode` (read-only + link/anchor
/// follow); other kinds currently get nothing -- their majors
/// already carry their full contribution set.
pub fn default_minor_mode_id_for_buffer_kind(kind: BufferKind) -> Option<ModeId> {
    match kind {
        BufferKind::Help => Some(HelpMode::mode_id()),
        BufferKind::Document
        | BufferKind::FileTree
        | BufferKind::Oil
        | BufferKind::Terminal => None,
    }
}

/// CSM.K1 (insert-completion.md §12): additional minor modes
/// the App activates alongside the buffer's major + the kind's
/// `default_minor`. Today: `completion-mode` on writable kinds
/// (Document) so `<C-Space>` opens the popup; read-only kinds
/// (Help, FileTree, Oil) don't activate it, so the trigger
/// chord is a silent no-op there.
pub fn auto_activated_minors_for_buffer_kind(kind: BufferKind) -> Vec<ModeId> {
    match kind {
        BufferKind::Document => vec![
            lattice_mode::CompletionMode::mode_id(),
            // CSM.4: buffer-words-mode auto-activates on every
            // Document; contributes the buffer-words source to
            // the popup's `ActiveCompletionSources` cache.
            lattice_mode::BufferWordsMode::mode_id(),
            // CSM.5: snippet-completion-mode owns the snippet
            // completion source; contributes through the same
            // cache.
            lattice_snippet::SnippetCompletionMode::mode_id(),
            // CSM.6: tree-sitter-completion-mode owns the
            // local-symbol source; produce-time is cheap (host
            // pre-walks `collect_symbols()` once per populate
            // and threads via `ctx.tree_sitter_symbols`).
            lattice_syntax::TreeSitterCompletionMode::mode_id(),
            // CSM.7: path-completion-mode owns the path source;
            // self-suppresses outside string scopes via
            // `ctx.path_context`.
            lattice_mode::PathCompletionMode::mode_id(),
        ],
        // Read-only kinds: nothing to add. (`help-mode` /
        // `oil-mode` / `file-tree-mode` are already activated
        // via their major's default-minor path.)
        BufferKind::Help
        | BufferKind::FileTree
        | BufferKind::Oil
        | BufferKind::Terminal => Vec::new(),
    }
}

/// M.4 follow-up: the per-kind modes (HelpMode, HoverMode,
/// FileTreeMode, OilMode) moved to `lattice_mode::modes` and
/// register through `lattice_mode::modes::register_foundation_modes`
/// alongside `TextMode`. This shim is kept as a no-op so existing
/// `register_buffer_kind_modes` callers don't break; the App's
/// boot path now calls `register_foundation_modes` directly and
/// drops this helper in a follow-up.
pub fn register_buffer_kind_modes(_registry: &mut ModeRegistry) {
    // intentionally empty
}

/// Resolve the major-mode id a buffer should activate based
/// on its kind + (for `Document` kinds) detected language.
/// Combines [`major_mode_id_for_buffer_kind`] with
/// [`lattice_syntax::major_mode_id_for_lang`], falling back to
/// [`TextMode`] when neither layer matches (`Document` +
/// `Lang::Plain`). M.3.1 wires this into the buffer-creation
/// path so each new buffer auto-activates its corresponding
/// major.
pub fn resolve_major_mode(kind: BufferKind, lang: Lang) -> ModeId {
    if let Some(id) = major_mode_id_for_buffer_kind(kind) {
        return id;
    }
    // Document kind: pick by language, fall through to text-mode.
    lattice_syntax::major_mode_id_for_lang(lang).unwrap_or_else(TextMode::mode_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_buffer_kind_mode_has_distinct_id() {
        let ids = [
            HelpMode::mode_id(),
            FileTreeMode::mode_id(),
            OilMode::mode_id(),
        ];
        for (i, a) in ids.iter().enumerate() {
            for b in &ids[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn buffer_kind_to_mode_id_table() {
        assert_eq!(major_mode_id_for_buffer_kind(BufferKind::Document), None);
        // M.4 (Option B): help buffers run markdown-mode as their
        // major; help-mode is layered on as a minor by the App's
        // activation path (see `default_minor_mode_id_for_buffer_kind`).
        assert_eq!(
            major_mode_id_for_buffer_kind(BufferKind::Help),
            Some(lattice_syntax::MarkdownMode::mode_id())
        );
        assert_eq!(
            default_minor_mode_id_for_buffer_kind(BufferKind::Help),
            Some(HelpMode::mode_id())
        );
        assert_eq!(
            major_mode_id_for_buffer_kind(BufferKind::FileTree),
            Some(FileTreeMode::mode_id())
        );
        assert_eq!(
            default_minor_mode_id_for_buffer_kind(BufferKind::FileTree),
            None
        );
        assert_eq!(
            major_mode_id_for_buffer_kind(BufferKind::Oil),
            Some(OilMode::mode_id())
        );
    }

    #[test]
    fn foundation_register_includes_per_kind_modes() {
        // M.4 follow-up: per-kind modes register through
        // `lattice_mode::modes::register_foundation_modes`
        // *and* the feature-crate-owned mode registration
        // helpers (`lattice_oil::register_oil_modes`, etc.).
        // App boot calls all of them; the test mirrors that
        // shape so the assertion set matches what an actual
        // boot produces.
        let mut registry = ModeRegistry::new();
        lattice_mode::register_foundation_modes(&mut registry);
        lattice_oil::register_oil_modes(&mut registry);
        lattice_file_tree::register_file_tree_modes(&mut registry);
        assert!(registry.is_registered(HelpMode::mode_id()));
        assert!(registry.is_registered(FileTreeMode::mode_id()));
        assert!(registry.is_registered(OilMode::mode_id()));
        assert!(registry.is_registered(HoverMode::mode_id()));
    }

    #[test]
    fn resolve_major_mode_combines_kind_and_lang() {
        // Help / FileTree / Oil ignore Lang -- their kind alone
        // determines the major. Help maps to markdown-mode
        // (Option B); help-mode is the minor.
        assert_eq!(
            resolve_major_mode(BufferKind::Help, Lang::Rust),
            lattice_syntax::MarkdownMode::mode_id()
        );
        assert_eq!(
            resolve_major_mode(BufferKind::FileTree, Lang::Plain),
            FileTreeMode::mode_id()
        );
        assert_eq!(
            resolve_major_mode(BufferKind::Oil, Lang::Markdown),
            OilMode::mode_id()
        );
    }

    #[test]
    fn resolve_major_mode_for_document_picks_by_lang() {
        assert_eq!(
            resolve_major_mode(BufferKind::Document, Lang::Rust),
            lattice_syntax::RustMode::mode_id()
        );
        assert_eq!(
            resolve_major_mode(BufferKind::Document, Lang::Markdown),
            lattice_syntax::MarkdownMode::mode_id()
        );
    }

    #[test]
    fn resolve_major_mode_falls_back_to_text_mode() {
        // Document + Plain ⇒ text-mode (foundation catch-all).
        assert_eq!(
            resolve_major_mode(BufferKind::Document, Lang::Plain),
            TextMode::mode_id()
        );
    }
}
