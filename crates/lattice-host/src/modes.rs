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

/// PU.1b-2a: generic per-buffer *static* highlight spans, merged into
/// the cells-worker `DisplayMatrix` ON TOP OF (overriding, by
/// first-match precedence) the live grammar spans on their byte
/// ranges. Indexed by source line. Unlike [`DocumentSyntax`] (the live
/// grammar handle, dynamic per edit) these are fixed for the buffer's
/// content — the home for styling the grammar can't derive because the
/// source text was transformed before parsing (help-link labels, whose
/// `[label](target)` markup is stripped before the markdown grammar
/// runs, so the grammar never emits a link capture). Property-derived,
/// NOT kind-specific: any buffer carrying this local gets the merge;
/// empty/absent is the universal default and renders byte-identically.
#[derive(Debug, Clone, Default)]
pub struct ExtraHighlights(pub Vec<Vec<lattice_syntax::StyledSpan>>);

impl BufferLocal for ExtraHighlights {
    const NAME: &'static str = "text-mode.extra-highlights";
    const DOC: &'static str = "Generic per-buffer static highlight spans merged on top of \
         the live grammar spans in the display matrix, indexed by source \
         line. The home for styling the grammar can't derive (e.g. help \
         links, whose markup is stripped before parsing). Empty is the \
         universal default and renders byte-identically.";
    const OWNER_MODE: &'static str = "text-mode";
    fn describe(&self) -> String {
        format!("{} line(s) of extra highlights", self.0.len())
    }
}

/// DR.3 (2026-08-12): per-buffer intra-line diff refinement — the byte
/// ranges whose BACKGROUND differs from their row's diff tint.
///
/// The second axis of `span-layering.md`, narrowed from per-row to
/// per-range. Published with [`ExtraHighlights`] by the same update and
/// spliced by the same loop, so the two cannot drift apart when an
/// inline expansion shifts lines.
///
/// Empty is the universal default and renders byte-identically.
#[derive(Debug, Clone, Default)]
pub struct ExtraRefinement(pub Vec<Vec<lattice_cells::RefineSpan>>);

impl BufferLocal for ExtraRefinement {
    const NAME: &'static str = "text-mode.extra-refinement";
    const DOC: &'static str = "Per-buffer intra-line diff refinement: byte ranges whose \
         background differs from their row's diff tint, indexed by source line. \
         Published alongside `extra-highlights` so the two shift together.";
    const OWNER_MODE: &'static str = "text-mode";
    fn describe(&self) -> String {
        format!("{} line(s) of refinement", self.0.len())
    }
}

/// DB.4: the content block width (widest line, in cells) for a buffer that
/// should be horizontally centred by widening its gutter. Set by the dashboard;
/// the renderer computes `left_pad = (viewport_width - width) / 2` and adds it
/// to the gutter, so content + cursor shift right (centred) with no text
/// mutation — markdown / links stay intact. Absent = not centred.
#[derive(Debug, Clone, Default)]
pub struct CenterContentWidth(pub u32);

impl BufferLocal for CenterContentWidth {
    const NAME: &'static str = "text-mode.center-content-width";
    const DOC: &'static str = "Content block width (cells) for gutter-based horizontal centring.";
    const OWNER_MODE: &'static str = "text-mode";
    fn describe(&self) -> String {
        format!("center to block width {}", self.0)
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

/// Resolve the default major-mode id for a [`BufferKind`] via
/// the registry's kind index (H.2).
///
/// `Document` returns `None` because the mode is determined by
/// language detection (see
/// [`lattice_syntax::major_mode_id_for_lang`]); when the language
/// detection returns `Lang::Plain` the caller falls back further
/// to `text-mode`.
///
/// All other kinds dispatch through
/// [`ModeRegistry::find_major_for_kind`]: each major mode
/// (`FileTreeMode`, `OilMode`, `MarkdownMode` for Help,
/// `MessagesMode`, `TerminalMode`, future plugin-defined majors)
/// declares its target kind via [`Mode::target_buffer_kind`] and
/// the registry indexes them at register-time. Adding a new
/// kind-bound major requires zero host-side hand edits — register
/// the mode and the index picks it up.
pub fn major_mode_id_for_buffer_kind(registry: &ModeRegistry, kind: BufferKind) -> Option<ModeId> {
    if kind == BufferKind::Document {
        // Document dispatches via `Lang` detection; the kind index
        // does not bind to `Document`.
        return None;
    }
    registry.find_major_for_kind(kind)
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
        // Dashboard's major (`dashboard-mode`) carries its own full
        // contribution set (read-only, gutterless); no default minor.
        BufferKind::Document
        | BufferKind::FileTree
        | BufferKind::Oil
        | BufferKind::Terminal
        | BufferKind::Messages
        | BufferKind::Multibuffer
        | BufferKind::Dashboard => None,
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
            // SN.3a: `snippet-completion-mode` is no longer
            // activated language-blind here. `snippet-mode` (the
            // language-aware gate) `implies` it and auto-activates
            // via the `MajorEntered` resolver (`ActivationPolicy`),
            // so the source rides the gate. With the default
            // `Global` policy this is behavior-preserving (the
            // source still reaches every Document), but SN.3b's
            // config can now restrict it per-language.
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
        | BufferKind::Terminal
        | BufferKind::Messages
        | BufferKind::Multibuffer
        | BufferKind::Dashboard => Vec::new(),
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

/// Resolve the major-mode id a buffer should activate based on
/// its kind + (for `Document` kinds) detected language.
///
/// H.2 (2026-05-31): rewritten to consult the [`ModeRegistry`]'s
/// kind index ([`major_mode_id_for_buffer_kind`]) for non-Document
/// kinds, then [`lattice_syntax::major_mode_id_for_lang`] for
/// Document-kind buffers, finally falling back to [`TextMode`]
/// when neither layer matches (Document + `Lang::Plain`, or an
/// unknown / unbound kind).
pub fn resolve_major_mode(registry: &ModeRegistry, kind: BufferKind, lang: Lang) -> ModeId {
    if let Some(id) = major_mode_id_for_buffer_kind(registry, kind) {
        return id;
    }
    // Document kind (or any kind with no registered major): pick
    // by language, fall through to text-mode.
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
        // H.2: lookups now go through the registry's kind index.
        // Foundation + feature-crate registration helpers must run
        // first so the index is populated.
        let mut registry = ModeRegistry::new();
        lattice_mode::register_foundation_modes(&mut registry);
        lattice_syntax::register_language_modes(&mut registry);
        lattice_oil::register_oil_modes(&mut registry);
        lattice_file_tree::register_file_tree_modes(&mut registry);
        lattice_terminal::register_terminal_modes(&mut registry);

        assert_eq!(
            major_mode_id_for_buffer_kind(&registry, BufferKind::Document),
            None
        );
        // M.4 (Option B): help buffers run markdown-mode as their
        // major; help-mode is layered on as a minor by the App's
        // activation path (see `default_minor_mode_id_for_buffer_kind`).
        assert_eq!(
            major_mode_id_for_buffer_kind(&registry, BufferKind::Help),
            Some(lattice_syntax::MarkdownMode::mode_id())
        );
        assert_eq!(
            default_minor_mode_id_for_buffer_kind(BufferKind::Help),
            Some(HelpMode::mode_id())
        );
        assert_eq!(
            major_mode_id_for_buffer_kind(&registry, BufferKind::FileTree),
            Some(FileTreeMode::mode_id())
        );
        assert_eq!(
            default_minor_mode_id_for_buffer_kind(BufferKind::FileTree),
            None
        );
        assert_eq!(
            major_mode_id_for_buffer_kind(&registry, BufferKind::Oil),
            Some(OilMode::mode_id())
        );
        assert_eq!(
            major_mode_id_for_buffer_kind(&registry, BufferKind::Messages),
            Some(lattice_mode::MessagesMode::mode_id())
        );
        assert_eq!(
            major_mode_id_for_buffer_kind(&registry, BufferKind::Terminal),
            Some(lattice_terminal::TerminalMode::mode_id())
        );
        // M.2.b.2 will register MultibufferMode through a
        // `lattice_multibuffer::register_multibuffer_modes` helper
        // and this assertion will flip to Some(...). Today the
        // multibuffer crate has no Mode, so the kind index is
        // empty for that slot.
        assert_eq!(
            major_mode_id_for_buffer_kind(&registry, BufferKind::Multibuffer),
            None
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

    fn populated_registry() -> ModeRegistry {
        let mut r = ModeRegistry::new();
        lattice_mode::register_foundation_modes(&mut r);
        lattice_syntax::register_language_modes(&mut r);
        lattice_oil::register_oil_modes(&mut r);
        lattice_file_tree::register_file_tree_modes(&mut r);
        lattice_terminal::register_terminal_modes(&mut r);
        r
    }

    #[test]
    fn resolve_major_mode_combines_kind_and_lang() {
        // Help / FileTree / Oil ignore Lang -- their kind alone
        // determines the major. Help maps to markdown-mode
        // (Option B); help-mode is the minor.
        let registry = populated_registry();
        assert_eq!(
            resolve_major_mode(&registry, BufferKind::Help, Lang::Rust),
            lattice_syntax::MarkdownMode::mode_id()
        );
        assert_eq!(
            resolve_major_mode(&registry, BufferKind::FileTree, Lang::Plain),
            FileTreeMode::mode_id()
        );
        assert_eq!(
            resolve_major_mode(&registry, BufferKind::Oil, Lang::Markdown),
            OilMode::mode_id()
        );
    }

    #[test]
    fn resolve_major_mode_for_document_picks_by_lang() {
        let registry = populated_registry();
        assert_eq!(
            resolve_major_mode(&registry, BufferKind::Document, Lang::Rust),
            lattice_syntax::RustMode::mode_id()
        );
        assert_eq!(
            resolve_major_mode(&registry, BufferKind::Document, Lang::Markdown),
            lattice_syntax::MarkdownMode::mode_id()
        );
    }

    #[test]
    fn resolve_major_mode_falls_back_to_text_mode() {
        // Document + Plain ⇒ text-mode (foundation catch-all).
        let registry = populated_registry();
        assert_eq!(
            resolve_major_mode(&registry, BufferKind::Document, Lang::Plain),
            TextMode::mode_id()
        );
    }

    #[test]
    fn resolve_major_mode_handles_empty_registry() {
        // Brand-new registry has no kind index entries — every
        // non-Document kind falls through to TextMode (the
        // language-detection catch-all on `Lang::Plain`).
        let registry = ModeRegistry::new();
        assert_eq!(
            resolve_major_mode(&registry, BufferKind::FileTree, Lang::Plain),
            TextMode::mode_id()
        );
    }
}

// MG.2: PendingSyntheticHighlights lives in lattice-mode now
// (crates/lattice-mode/src/pending_synthetic_highlights.rs).
// No import needed here — it's accessed via lattice_mode::PendingSyntheticHighlights
// in dispatch.rs and editor_boot.rs.
